module Page.Home exposing (Model, Msg, init, subscriptions, update, view)

import Api exposing (Library, LibraryKind(..))
import Browser.Navigation as Nav
import Html exposing (..)
import Html.Attributes exposing (..)
import Html.Events exposing (onInput, onSubmit)
import Route
import Ui


type alias Model =
    { key : Nav.Key
    , libraries : Libraries
    , query : String
    }


type Libraries
    = Loading
    | Loaded (List Library)
    | Failed String


type Msg
    = GotLibraries (Result Api.Error (List Library))
    | QueryInput String
    | QuerySubmit


init : Nav.Key -> ( Model, Cmd Msg )
init key =
    ( { key = key, libraries = Loading, query = "" }
    , Api.getLibraries GotLibraries
    )


subscriptions : Model -> Sub Msg
subscriptions _ =
    Sub.none


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        GotLibraries (Ok libraries) ->
            ( { model | libraries = Loaded libraries }, Cmd.none )

        GotLibraries (Err err) ->
            ( { model | libraries = Failed (Api.errorToString err) }
            , Cmd.none
            )

        QueryInput query ->
            ( { model | query = query }, Cmd.none )

        QuerySubmit ->
            if String.isEmpty (String.trim model.query) then
                ( model, Cmd.none )

            else
                ( model
                , Nav.pushUrl model.key
                    (Route.toString
                        (Route.Search
                            { query = model.query
                            , library = Nothing
                            , page = 1
                            }
                        )
                    )
                )


view : Model -> Html Msg
view model =
    div [ class "page" ]
        [ h1 [] [ text "Loku" ]
        , viewSearch model.query
        , case model.libraries of
            Loading ->
                p [] [ text "Loading libraries…" ]

            Failed err ->
                Ui.errorText err

            Loaded libraries ->
                div [ class "library-grid" ]
                    (List.map viewLibraryCard libraries)
        , nav [ class "home-tools" ]
            [ a [ href (Route.toString Route.Rips) ]
                [ text "Disc rips and identification" ]
            ]
        ]


viewSearch : String -> Html Msg
viewSearch query =
    Html.form
        [ onSubmit QuerySubmit, class "search-form search-form-home" ]
        [ input
            [ type_ "search"
            , placeholder "Search all libraries…"
            , value query
            , onInput QueryInput
            , class "search-input"
            ]
            []
        , button [ type_ "submit", class "search-button" ] [ text "Search" ]
        ]


viewLibraryCard : Library -> Html Msg
viewLibraryCard library =
    a
        [ href
            (Route.toString
                (Route.Browse { library = library.name, path = "", page = 1 })
            )
        , class "library-card"
        ]
        [ div [ class "library-card-icon" ] [ text (kindIcon library.kind) ]
        , div [ class "library-card-name" ] [ text library.name ]
        , div [ class "library-card-kind" ] [ text (kindLabel library.kind) ]
        ]


kindIcon : LibraryKind -> String
kindIcon kind =
    case kind of
        Downloads ->
            "📼"

        Discs ->
            "💿"


kindLabel : LibraryKind -> String
kindLabel kind =
    case kind of
        Downloads ->
            "Downloads"

        Discs ->
            "Disc rips"
