port module Main exposing (main)

import Browser
import Browser.Navigation as Nav
import Html exposing (..)
import Html.Attributes exposing (..)
import Page.Browse as Browse
import Page.Home as Home
import Page.Player as Player
import Page.Search as Search
import Route exposing (Route(..))
import Url exposing (Url)


port logError : String -> Cmd msg


port videoProgress : (Float -> msg) -> Sub msg


type Page
    = HomePage Home.Model
    | BrowsePage Browse.Model
    | SearchPage Search.Model
    | PlayerPage Player.Model
    | NotFoundPage


type alias Flags =
    {}


type alias Model =
    { key : Nav.Key
    , page : Page
    }


type Msg
    = UrlRequested Browser.UrlRequest
    | UrlChanged Url
    | HomeMsg Home.Msg
    | BrowseMsg Browse.Msg
    | SearchMsg Search.Msg
    | PlayerMsg Player.Msg


main : Program Flags Model Msg
main =
    Browser.application
        { init = init
        , view = view
        , update = update
        , subscriptions = \_ -> videoProgress (PlayerMsg << Player.VideoProgress)
        , onUrlRequest = UrlRequested
        , onUrlChange = UrlChanged
        }


init : Flags -> Url -> Nav.Key -> ( Model, Cmd Msg )
init _ url key =
    routeToPage key (Route.parse url)
        |> Tuple.mapFirst (\page -> { key = key, page = page })


routeToPage : Nav.Key -> Route -> ( Page, Cmd Msg )
routeToPage key route =
    case route of
        Route.Home ->
            Home.init key
                |> Tuple.mapBoth HomePage (Cmd.map HomeMsg)

        Route.Browse params ->
            Browse.init key params
                |> Tuple.mapBoth BrowsePage (Cmd.map BrowseMsg)

        Route.Search params ->
            Search.init key params
                |> Tuple.mapBoth SearchPage (Cmd.map SearchMsg)

        Route.Player params ->
            Player.init params
                |> Tuple.mapBoth PlayerPage (Cmd.map PlayerMsg)

        Route.NotFound ->
            ( NotFoundPage, Cmd.none )


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        UrlRequested (Browser.Internal url) ->
            ( model, Nav.pushUrl model.key (Url.toString url) )

        UrlRequested (Browser.External url) ->
            ( model, Nav.load url )

        UrlChanged url ->
            case Route.parse url of
                Route.Browse params ->
                    case model.page of
                        BrowsePage subModel ->
                            if
                                Browse.currentLocation subModel
                                    == ( params.library, params.path )
                            then
                                -- Same directory: update the page number
                                -- without re-fetching.
                                Browse.updateParams params subModel
                                    |> Tuple.mapFirst
                                        (\m -> { model | page = BrowsePage m })
                                    |> Tuple.mapSecond (Cmd.map BrowseMsg)

                            else
                                Browse.init model.key params
                                    |> Tuple.mapBoth
                                        (\m -> { model | page = BrowsePage m })
                                        (Cmd.map BrowseMsg)

                        _ ->
                            Browse.init model.key params
                                |> Tuple.mapBoth
                                    (\m -> { model | page = BrowsePage m })
                                    (Cmd.map BrowseMsg)

                route ->
                    routeToPage model.key route
                        |> Tuple.mapFirst (\page -> { model | page = page })

        HomeMsg subMsg ->
            case model.page of
                HomePage subModel ->
                    Home.update subMsg subModel
                        |> Tuple.mapBoth
                            (\m -> { model | page = HomePage m })
                            (Cmd.map HomeMsg)

                _ ->
                    ( model, Cmd.none )

        BrowseMsg subMsg ->
            case model.page of
                BrowsePage subModel ->
                    Browse.update subMsg subModel
                        |> Tuple.mapBoth
                            (\m -> { model | page = BrowsePage m })
                            (Cmd.map BrowseMsg)

                _ ->
                    ( model, Cmd.none )

        SearchMsg subMsg ->
            case model.page of
                SearchPage subModel ->
                    Search.update subMsg subModel
                        |> Tuple.mapBoth
                            (\m -> { model | page = SearchPage m })
                            (Cmd.map SearchMsg)

                _ ->
                    ( model, Cmd.none )

        PlayerMsg (Player.MediaError code) ->
            case model.page of
                PlayerPage subModel ->
                    Player.update (Player.MediaError code) subModel
                        |> Tuple.mapBoth
                            (\m -> { model | page = PlayerPage m })
                            (\cmd ->
                                Cmd.batch
                                    [ Cmd.map PlayerMsg cmd
                                    , logError (Player.mediaErrorMessage code)
                                    ]
                            )

                _ ->
                    ( model, Cmd.none )

        PlayerMsg subMsg ->
            case model.page of
                PlayerPage subModel ->
                    Player.update subMsg subModel
                        |> Tuple.mapBoth
                            (\m -> { model | page = PlayerPage m })
                            (Cmd.map PlayerMsg)

                _ ->
                    ( model, Cmd.none )


view : Model -> Browser.Document Msg
view model =
    { title = "Loku"
    , body =
        [ div [ class "app-shell" ]
            [ case model.page of
                HomePage subModel ->
                    Html.map HomeMsg (Home.view subModel)

                BrowsePage subModel ->
                    Html.map BrowseMsg (Browse.view subModel)

                SearchPage subModel ->
                    Html.map SearchMsg (Search.view subModel)

                PlayerPage subModel ->
                    Html.map PlayerMsg (Player.view subModel)

                NotFoundPage ->
                    p [ class "not-found" ] [ text "Page not found." ]
            ]
        ]
    }
